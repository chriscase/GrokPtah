import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { GrokAccountBadge } from "./GrokAccountBadge";
import {
  GROK_ACCOUNT_CONTRACT,
  GROK_ACCOUNT_SCHEMA_VERSION,
  absentGrokAccountFacts,
  type GrokAccountFacts,
} from "../lib/grokAccountFacts";

const SENTINEL_BEARER = "xai-SENTINEL-BEARER-DO-NOT-LEAK";

// This project runs vitest without `globals`, so Testing Library's automatic
// afterEach cleanup never registers. Unmount explicitly or queries below would
// see every previously rendered badge.
afterEach(cleanup);

function facts(overrides: Partial<GrokAccountFacts> = {}): GrokAccountFacts {
  return {
    contract: GROK_ACCOUNT_CONTRACT,
    schemaVersion: GROK_ACCOUNT_SCHEMA_VERSION,
    credentialMethod: "grok_build_oidc",
    accountReference: { value: "usr-0a1b2c3d", source: "user_id" },
    expiry: { status: "valid", expiresAt: "2026-08-25T12:30:00Z", secondsRemaining: 45_000 },
    readiness: "usable",
    readinessReason: "expiry_in_future",
    ...overrides,
  };
}

const EXPIRED = facts({
  expiry: { status: "expired", expiresAt: "2026-08-24T23:59:59Z", secondsRemaining: -1 },
  readiness: "unusable",
  readinessReason: "credential_expired",
});

const UNKNOWN_EXPIRY = facts({
  expiry: { status: "absent", expiresAt: null, secondsRemaining: null },
  readiness: "unknown",
  readinessReason: "expiry_not_provided",
});

describe("GrokAccountBadge states", () => {
  it("shows a ready account with its safe route and account reference", async () => {
    render(<GrokAccountBadge facts={facts()} />);
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Ready");
    expect(trigger).toHaveTextContent("Grok Build sign-in");
    expect(trigger).toHaveTextContent("usr-0a1b2c3d");

    await userEvent.click(trigger);
    expect(screen.getByRole("button", { expanded: true })).toBe(trigger);
    expect(screen.getByText(/expiring in 12h 30m/)).toBeInTheDocument();
    // A healthy account offers no recovery affordance and blocks nothing.
    expect(screen.queryByRole("button", { name: /refresh grok build/i })).not.toBeInTheDocument();
    expect(screen.queryByText(/New runs are disabled/)).not.toBeInTheDocument();
  });

  it("explains unknown expiry as unknown rather than expired, and does not block", async () => {
    render(<GrokAccountBadge facts={UNKNOWN_EXPIRY} onReauthenticate={vi.fn()} />);
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Unknown");
    expect(trigger).toHaveTextContent("expiry unknown");

    await userEvent.click(trigger);
    expect(screen.getByText(/unknown, not expired/)).toBeInTheDocument();
    expect(screen.queryByText(/New runs are disabled/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /refresh grok build/i })).not.toBeInTheDocument();
    // Non-blocking states must not hijack the live region.
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("blocks on an expired session and offers a re-authentication path", async () => {
    const onReauthenticate = vi.fn();
    render(<GrokAccountBadge facts={EXPIRED} onReauthenticate={onReauthenticate} />);
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Blocked");
    expect(trigger).toHaveTextContent("Session expired");

    await userEvent.click(trigger);
    const detail = screen.getByRole("status");
    expect(detail).toHaveAttribute("aria-live", "polite");
    expect(within(detail).getByText(/expired 1s ago/)).toBeInTheDocument();
    expect(within(detail).getByText(/Sign in to Grok Build again/)).toBeInTheDocument();
    // Existing runs stay inspectable; only new launches are gated.
    expect(within(detail).getByText(/Runs already recorded stay open for inspection/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /refresh grok build/i }));
    expect(onReauthenticate).toHaveBeenCalledTimes(1);
  });

  it("blocks when no credential is present at all", async () => {
    render(<GrokAccountBadge facts={absentGrokAccountFacts()} onReauthenticate={vi.fn()} />);
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Blocked");
    expect(trigger).toHaveTextContent("Not signed in");
    await userEvent.click(trigger);
    expect(screen.getByText(/No Grok Build credential was found/)).toBeInTheDocument();
  });

  it("blocks when the host reported facts this build cannot validate", async () => {
    render(<GrokAccountBadge facts={null} onReauthenticate={vi.fn()} />);
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Blocked");
    expect(trigger).toHaveTextContent("Account status unreadable");
    await userEvent.click(trigger);
    expect(screen.getByText(/will not vouch for the credential/)).toBeInTheDocument();
  });

  it("makes no readiness claim for an unrecognized route, and does not block", async () => {
    render(
      <GrokAccountBadge
        facts={facts({
          credentialMethod: "unknown",
          readiness: "unknown",
          readinessReason: "method_unrecognized",
        })}
      />,
    );
    const trigger = screen.getByRole("button", { expanded: false });
    expect(trigger).toHaveTextContent("Unknown");
    expect(trigger).toHaveTextContent("Route unrecognized");
    await userEvent.click(trigger);
    expect(screen.getByText(/no readiness claim is made/)).toBeInTheDocument();
    expect(screen.queryByText(/New runs are disabled/)).not.toBeInTheDocument();
  });
});

describe("GrokAccountBadge accessibility boundaries", () => {
  it("is reachable and operable from the keyboard alone", async () => {
    render(<GrokAccountBadge facts={EXPIRED} onReauthenticate={vi.fn()} />);
    const trigger = screen.getByRole("button", { expanded: false });

    await userEvent.tab();
    expect(trigger).toHaveFocus();

    // Enter and Space both toggle a native button.
    await userEvent.keyboard("{Enter}");
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    await userEvent.keyboard(" ");
    expect(trigger).toHaveAttribute("aria-expanded", "false");

    // The recovery affordance is the next stop in DOM order once revealed.
    await userEvent.keyboard("{Enter}");
    await userEvent.tab();
    expect(screen.getByRole("button", { name: /refresh grok build/i })).toHaveFocus();
  });

  it("ties the trigger to the region it controls", async () => {
    render(<GrokAccountBadge facts={facts()} />);
    const trigger = screen.getByRole("button", { expanded: false });
    const controls = trigger.getAttribute("aria-controls");
    expect(controls).toBeTruthy();
    const detail = document.getElementById(controls as string);
    expect(detail).not.toBeNull();
    // Collapsed detail is hidden from the accessibility tree, not just visually.
    expect(detail).toHaveAttribute("hidden");
    await userEvent.click(trigger);
    expect(detail).not.toHaveAttribute("hidden");
  });

  it("carries tone in text, not colour alone, for forced-colors and monochrome", () => {
    for (const [projection, tone] of [
      [facts(), "Ready"],
      [UNKNOWN_EXPIRY, "Unknown"],
      [EXPIRED, "Blocked"],
    ] as const) {
      const { unmount } = render(<GrokAccountBadge facts={projection} />);
      expect(screen.getByRole("button", { expanded: false })).toHaveTextContent(tone);
      // The colour swatch itself is decorative and never announced.
      const dot = document.querySelector(".grok-account-dot");
      expect(dot).toHaveAttribute("aria-hidden", "true");
      unmount();
    }
  });

  it("keeps every rendered string free of credential material", async () => {
    for (const projection of [facts(), EXPIRED, UNKNOWN_EXPIRY, absentGrokAccountFacts(), null]) {
      const { container, unmount } = render(
        <GrokAccountBadge facts={projection} onReauthenticate={vi.fn()} />,
      );
      await userEvent.click(screen.getByRole("button", { expanded: false }));
      // Scan the whole subtree, including the accessibility-relevant attributes.
      const rendered = container.innerHTML;
      for (const needle of [
        SENTINEL_BEARER,
        "bearer",
        "Bearer",
        "refresh_token",
        "refreshToken",
        "apiKey",
        "XAI_API_KEY",
        "keychain:",
        "auth_mode",
        "@example.test",
        "/Users/",
        "/private/",
      ]) {
        expect(rendered).not.toContain(needle);
      }
      unmount();
    }
  });

  it("uses relative units so 200% text reflows instead of clipping", () => {
    // Guards against a regression to fixed px sizing in the badge markup.
    const { container } = render(<GrokAccountBadge facts={facts()} />);
    expect(container.querySelector("[style]")).toBeNull();
  });
});
