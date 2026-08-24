import { describe, expect, it } from "vitest";
import {
  CAPABILITY_CONTRACT,
  capabilityActionState,
  findCapability,
  parseCapabilitySet,
} from "./capabilities";

const payload = {
  contract: CAPABILITY_CONTRACT,
  capabilities: [
    {
      id: "run.execute",
      tier: "execute",
      mutating: true,
      human_gate: false,
      availability: "available",
      description: "Submit bounded Build runs.",
    },
    {
      id: "computer.control",
      tier: "computer_control",
      mutating: true,
      human_gate: true,
      availability: "gated",
      description: "Use lease-fenced semantic controls.",
    },
  ],
};

describe("capability discovery", () => {
  it("parses the versioned contract and finds stable ids", () => {
    const set = parseCapabilitySet(payload);
    expect(set).not.toBeNull();
    expect(findCapability(set, "run.execute")?.mutating).toBe(true);
  });

  it("fails closed for an unknown contract or malformed descriptor", () => {
    expect(parseCapabilitySet({ ...payload, contract: "grokptah.capabilities.v2" })).toBeNull();
    expect(
      parseCapabilitySet({
        ...payload,
        capabilities: [{ ...payload.capabilities[0], tier: "admin" }],
      }),
    ).toBeNull();
  });

  it("keeps gated controls visible but disabled until approval", () => {
    const set = parseCapabilitySet(payload);
    const control = findCapability(set, "computer.control");
    expect(capabilityActionState(control)).toBe("requires_gate");
    expect(capabilityActionState(control, true)).toBe("ready");
    expect(capabilityActionState(undefined)).toBe("unavailable");
  });
});
