import { describe, expect, it } from "vitest";
import {
  canSplitSessions,
  densityFromWidth,
  SPLIT_MIN_WIDTH,
} from "./layout";

describe("densityFromWidth", () => {
  it("enables wide (split) at SPLIT_MIN_WIDTH", () => {
    expect(densityFromWidth(SPLIT_MIN_WIDTH - 1)).toBe("normal");
    expect(densityFromWidth(SPLIT_MIN_WIDTH)).toBe("wide");
    expect(densityFromWidth(1600)).toBe("wide");
    expect(densityFromWidth(2000)).toBe("ultrawide");
  });
});

describe("canSplitSessions", () => {
  it("is true only for wide and ultrawide", () => {
    expect(canSplitSessions("compact")).toBe(false);
    expect(canSplitSessions("normal")).toBe(false);
    expect(canSplitSessions("wide")).toBe(true);
    expect(canSplitSessions("ultrawide")).toBe(true);
  });
});
