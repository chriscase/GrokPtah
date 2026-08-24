import { describe, expect, it } from "vitest";
import {
  HELP_CONTRACT,
  buildHelpAssistantContext,
  searchHelp,
} from "./help";

describe("Help Center index", () => {
  it("ranks a natural-language recovery query", () => {
    const hits = searchHelp("why did my long running agent duplicate after restart?");
    expect(hits[0]?.entry.id).toBe("durable-runs-and-recovery");
    expect(hits[0]?.matchedTerms).toContain("restart");
  });

  it("keeps gated/operator help out of the public default", () => {
    const publicHits = searchHelp("company gateway review");
    expect(publicHits.some((hit) => hit.entry.id === "enterprise-gateway-review")).toBe(false);
    const operatorHits = searchHelp("company gateway review", {
      includeRestricted: true,
      audience: "operator",
    });
    expect(operatorHits[0]?.entry.id).toBe("enterprise-gateway-review");
  });

  it("filters by capability without granting it", () => {
    const hits = searchHelp("control desktop safely", {
      includeRestricted: true,
      capabilityIds: ["computer.control"],
    });
    expect(hits[0]?.entry.id).toBe("computer-use-safety");
    expect(hits[0]?.entry.access).toBe("gated");
  });

  it("builds bounded assistant context with an explicit authority boundary", () => {
    const context = buildHelpAssistantContext("semantic help search");
    expect(context.contract).toBe(HELP_CONTRACT);
    expect(context.hits.length).toBeLessThanOrEqual(5);
    expect(context.instruction).toMatch(/fresh scoped check/i);
  });
});
