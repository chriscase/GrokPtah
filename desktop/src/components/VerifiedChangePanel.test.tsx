import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { VerifiedChangePanel } from "./VerifiedChangePanel";

afterEach(() => cleanup());

describe("VerifiedChangePanel", () => {
  it("shows actionable unreadiness without claiming a dispatch", () => {
    render(
      <VerifiedChangePanel
        view={{
          repositoryId: "repo-verified-change",
          sourceRevision: "abc123",
          agentId: "agent-1",
          executionHost: "desktop",
          allowedFiles: ["src/ledger.rs", "src/report.rs"],
          executor: "grok_build_isolated_review",
          modelSelectionKey: "agent-route",
          cliVersion: null,
          limits: { maxRounds: 8, maxDurationMs: 300000 },
          requiredChecks: [{ checkId: "balance-regression", cwd: "oracle", timeoutMs: 2000 }],
          approvalRequired: true,
          readiness: {
            ready: false,
            reasons: ["The managed Grok CLI executable is missing."],
            workersDispatched: 0,
            providerInvocations: 0,
          },
          safeAction: "Resolve readiness, then start one supervised attempt.",
        }}
      />,
    );
    expect(screen.getByText("repo-verified-change")).toBeTruthy();
    expect(screen.getByText(/not ready/)).toBeTruthy();
    expect(screen.getByText(/0 workers/)).toBeTruthy();
    expect(screen.getByText(/CLI executable is missing/)).toBeTruthy();
  });
});
