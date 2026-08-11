import { describe, expect, it } from "vitest";
import { appendThoughtChunk } from "./thoughtText";

describe("appendThoughtChunk", () => {
  it("joins natural-language chunks with spaces", () => {
    expect(
      appendThoughtChunk(
        "The user wants me to inspect",
        "git diff --check after the queued test run",
      ),
    ).toBe("The user wants me to inspect git diff --check after the queued test run");
  });

  it("does not add a space before punctuation or duplicate chunks", () => {
    expect(appendThoughtChunk("The task is complete", ".")).toBe(
      "The task is complete.",
    );
    expect(appendThoughtChunk("The task is complete.", ".")).toBe(
      "The task is complete.",
    );
  });

  it("keeps diagnostic lines separate", () => {
    expect(
      appendThoughtChunk("kind=build model=grok-4.5 effort=high", "auth=oidc"),
    ).toBe("kind=build model=grok-4.5 effort=high\nauth=oidc");
  });
});
