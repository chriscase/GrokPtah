import { describe, expect, it } from "vitest";
import {
  canSplitSessions,
  clampDocks,
  densityFromWidth,
  maxDocksForDensity,
  maxDocksFromStageWidth,
  MAX_DOCKS,
  SPLIT_MIN_WIDTH,
  DOCK_STAGE_2,
  DOCK_STAGE_3,
  DOCK_STAGE_4,
  DOCK_STAGE_5,
  DOCK_STAGE_6,
} from "./layout";

describe("densityFromWidth", () => {
  it("enables wide at SPLIT_MIN_WIDTH", () => {
    expect(densityFromWidth(SPLIT_MIN_WIDTH - 1)).toBe("normal");
    expect(densityFromWidth(SPLIT_MIN_WIDTH)).toBe("wide");
    expect(densityFromWidth(1600)).toBe("wide");
    expect(densityFromWidth(2000)).toBe("ultrawide");
  });
});

describe("maxDocksFromStageWidth", () => {
  it("returns 1–6 at stage thresholds", () => {
    expect(maxDocksFromStageWidth(DOCK_STAGE_2 - 1)).toBe(1);
    expect(maxDocksFromStageWidth(DOCK_STAGE_2)).toBe(2);
    expect(maxDocksFromStageWidth(DOCK_STAGE_3)).toBe(3);
    expect(maxDocksFromStageWidth(DOCK_STAGE_4)).toBe(4);
    expect(maxDocksFromStageWidth(DOCK_STAGE_5)).toBe(5);
    expect(maxDocksFromStageWidth(DOCK_STAGE_6)).toBe(6);
  });

  it("fits 6 on a typical 3840 ultrawide stage", () => {
    // Full width minus modest chrome still clears DOCK_STAGE_6
    expect(maxDocksFromStageWidth(3400)).toBe(6);
    expect(maxDocksFromStageWidth(2520)).toBe(6);
    expect(maxDocksFromStageWidth(2519)).toBe(5);
  });
});

describe("maxDocksForDensity", () => {
  it("maps density to dock capacity fallback", () => {
    expect(maxDocksForDensity("compact")).toBe(1);
    expect(maxDocksForDensity("normal")).toBe(1);
    expect(maxDocksForDensity("wide")).toBe(3);
    expect(maxDocksForDensity("ultrawide")).toBe(5);
  });
});

describe("MAX_DOCKS", () => {
  it("caps at 6", () => {
    expect(MAX_DOCKS).toBe(6);
  });
});

describe("canSplitSessions", () => {
  it("is true when density allows 2+ docks", () => {
    expect(canSplitSessions("compact")).toBe(false);
    expect(canSplitSessions("normal")).toBe(false);
    expect(canSplitSessions("wide")).toBe(true);
    expect(canSplitSessions("ultrawide")).toBe(true);
  });
});

describe("clampDocks", () => {
  it("keeps focused when shrinking", () => {
    expect(clampDocks(["a", "b", "c"], 2, "c")).toEqual(["c", "a"]);
  });
  it("dedupes and respects max", () => {
    expect(clampDocks(["a", "a", "b"], 2, "a")).toEqual(["a", "b"]);
  });
  it("returns empty when max < 1", () => {
    expect(clampDocks(["a"], 0, "a")).toEqual([]);
  });
  it("can hold six", () => {
    expect(
      clampDocks(["a", "b", "c", "d", "e", "f", "g"], 6, "d"),
    ).toEqual(["d", "a", "b", "c", "e", "f"]);
  });
});
