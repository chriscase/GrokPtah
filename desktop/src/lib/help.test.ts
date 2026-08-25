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
    expect(context.contextBytes).toBeLessThanOrEqual(context.maxBytes);
    expect(context.truncated).toBe(false);
  });

  it("enforces a caller-selected UTF-8 byte bound and reports truncation", () => {
    const context = buildHelpAssistantContext("computer use safety", {
      includeRestricted: true,
      maxBytes: 2_048,
    });
    expect(context.contextBytes).toBeLessThanOrEqual(2_048);
    expect(context.truncated).toBe(true);
    expect(context.hits.length).toBeGreaterThan(0);
    expect(context.hits.some(({ entry }) => entry.id === "computer-use-safety")).toBe(true);
  });

  it("does not split multibyte query text when applying the bound", () => {
    const query = "日本語コンピューター利用".repeat(100);
    const context = buildHelpAssistantContext(query, { maxBytes: 2_048 });
    expect(context.contextBytes).toBeLessThanOrEqual(2_048);
    expect(() => JSON.stringify(context)).not.toThrow();
  });

  it("keeps the envelope bounded even when the caller supplies a tiny limit", () => {
    const context = buildHelpAssistantContext("x".repeat(1_000), { maxBytes: 512 });
    expect(context.contextBytes).toBeLessThanOrEqual(512);
    expect(context.query.length).toBeLessThan(1_000);
  });
});
