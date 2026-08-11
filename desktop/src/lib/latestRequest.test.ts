import { describe, expect, it } from "vitest";
import { createLatestRequestGuard } from "./latestRequest";

describe("createLatestRequestGuard", () => {
  it("invalidates an older refresh when a newer one starts", () => {
    const guard = createLatestRequestGuard();
    const first = guard.begin();
    const second = guard.begin();

    expect(guard.isCurrent(first)).toBe(false);
    expect(guard.isCurrent(second)).toBe(true);
  });
});
