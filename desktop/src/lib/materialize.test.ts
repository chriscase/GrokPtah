import { describe, expect, it } from "vitest";
import { materializeBatchSize, tokenizeForMaterialize } from "./materialize";

describe("tokenizeForMaterialize", () => {
  it("keeps whitespace with words", () => {
    expect(tokenizeForMaterialize("Hello world\n")).toEqual([
      "Hello ",
      "world\n",
    ]);
  });
  it("handles empty", () => {
    expect(tokenizeForMaterialize("")).toEqual([]);
  });
});

describe("materializeBatchSize", () => {
  it("speeds up on backlog", () => {
    expect(materializeBatchSize(1)).toBe(1);
    expect(materializeBatchSize(20)).toBe(3);
    expect(materializeBatchSize(100)).toBe(10);
  });
});
