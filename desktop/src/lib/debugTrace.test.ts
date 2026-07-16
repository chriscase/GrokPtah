import { describe, expect, it } from "vitest";
import { isDebugLine, isDebugThought } from "./debugTrace";

describe("isDebugLine", () => {
  it("classifies host status crumbs", () => {
    expect(
      isDebugLine(
        "kind=build model=grok-4.5 effort=high auth=Chris via grok_build:oidc",
      ),
    ).toBe(true);
    expect(
      isDebugLine(
        "agent loop starting (tools on, max 24 rounds, effort=high, sandbox=workspace-write) as grok_build:oidc…",
      ),
    ).toBe(true);
    expect(isDebugLine("agent round 1/24…")).toBe(true);
    expect(isDebugLine("tool `read_file`…")).toBe(true);
  });

  it("does not classify real assistant prose", () => {
    expect(
      isDebugLine("I'll look at the host module and clean up the debug crumbs."),
    ).toBe(false);
  });
});

describe("isDebugThought", () => {
  it("collapses multi-line agent loop noise", () => {
    const text = [
      "kind=build model=grok-4.5 effort=high auth=Chris via grok_build:oidc",
      "agent loop starting (tools on, max 24 rounds) as grok_build:oidc…",
      "agent round 1/24…",
    ].join("\n");
    expect(isDebugThought(text)).toBe(true);
  });
});
