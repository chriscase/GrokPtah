import { describe, expect, it } from "vitest";
import type { CodingWorktreeHostView } from "./protocol";
import { codingWorktreeAffordances, codingWorktreeStatus } from "./codingWorktreeDisposition";

function view(overrides: Partial<CodingWorktreeHostView> = {}): CodingWorktreeHostView {
  return {
    handle: "wcr-1",
    agentSessionId: "session-1",
    identity: {
      sessionId: "wcr-1",
      baseSha: "abc",
      worktreePathDigest: "sha256:wt",
      branchName: "wcr/wcr-1",
    },
    phase: "active",
    disposition: null,
    pauseFenced: false,
    settlementFenced: false,
    applyUncertain: false,
    worktreePath: "/tmp/wt",
    patchDigest: "sha256:patch",
    ...overrides,
  };
}

describe("codingWorktreeAffordances", () => {
  it("disables every action when no session is attached", () => {
    const next = codingWorktreeAffordances(null, "/tmp/apply", "sha256:patch");
    expect(next.status).toBe("none");
    expect(next.canPause).toBe(false);
    expect(next.canStop).toBe(false);
    expect(next.canAccept).toBe(false);
    expect(next.canDiscard).toBe(false);
    expect(next.canKeepForReview).toBe(false);
    expect(next.reason).toMatch(/No coding worktree session/);
  });

  it("keeps Accept off until digest and apply target are explicit", () => {
    const staged = view();
    expect(codingWorktreeAffordances(staged, "", "sha256:patch").canAccept).toBe(false);
    expect(codingWorktreeAffordances(staged, "/tmp/apply", "  ").canAccept).toBe(false);
    expect(codingWorktreeAffordances(staged, "/tmp/apply", "sha256:patch").canAccept).toBe(
      true,
    );
    expect(codingWorktreeAffordances(view({ patchDigest: null }), "/tmp/apply", "").canKeepForReview).toBe(
      false,
    );
    expect(codingWorktreeAffordances(view({ patchDigest: null }), "", "").canDiscard).toBe(
      true,
    );
    expect(codingWorktreeAffordances(staged, "/tmp/apply", "sha256:patch").canPause).toBe(true);
    expect(codingWorktreeAffordances(staged, "/tmp/apply", "sha256:patch").canStop).toBe(true);
  });

  it("matches Paused / Uncertain / Settled host fences", () => {
    const paused = codingWorktreeAffordances(
      view({ phase: "paused", pauseFenced: true }),
      "/tmp/apply",
      "sha256:patch",
    );
    expect(paused.status).toBe("paused");
    expect(paused.canPause).toBe(false);
    expect(paused.canAccept).toBe(false);
    expect(paused.canDiscard).toBe(false);
    expect(paused.canKeepForReview).toBe(false);
    expect(paused.canStop).toBe(true);

    const uncertain = codingWorktreeAffordances(
      view({
        phase: "settled",
        disposition: "uncertain",
        applyUncertain: true,
        pauseFenced: true,
        settlementFenced: true,
      }),
      "/tmp/apply",
      "sha256:patch",
    );
    expect(
      codingWorktreeStatus(
        view({
          phase: "settled",
          disposition: "uncertain",
          applyUncertain: true,
        }),
      ),
    ).toBe("uncertain");
    expect(uncertain.status).toBe("uncertain");
    expect(uncertain.canAccept).toBe(false);
    expect(uncertain.canPause).toBe(false);
    expect(uncertain.canStop).toBe(true);
    expect(uncertain.reason).toMatch(/Auto-retry and auto-Accept are forbidden/);

    const settled = codingWorktreeAffordances(
      view({
        phase: "settled",
        disposition: "accepted",
        pauseFenced: true,
        settlementFenced: true,
      }),
      "/tmp/apply",
      "sha256:patch",
    );
    expect(settled.canPause).toBe(false);
    expect(settled.canAccept).toBe(false);
    expect(settled.canDiscard).toBe(false);
    expect(settled.canKeepForReview).toBe(false);
    expect(settled.canStop).toBe(true);

    const destroyed = codingWorktreeAffordances(
      view({
        phase: "destroyed",
        disposition: "uncertain",
        applyUncertain: true,
        pauseFenced: true,
        settlementFenced: true,
      }),
      "/tmp/apply",
      "sha256:patch",
    );
    expect(destroyed.status).toBe("destroyed");
    expect(destroyed.canStop).toBe(false);
  });
});
