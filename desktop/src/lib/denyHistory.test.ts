import { describe, expect, it, beforeEach } from "vitest";
import {
  appendDeny,
  clearDenyHistory,
  loadDenyHistory,
} from "./denyHistory";

describe("denyHistory (#175)", () => {
  beforeEach(() => {
    clearDenyHistory();
  });

  it("appends and loads deny entries", () => {
    const next = appendDeny([], {
      tool_name: "run_terminal_cmd",
      summary: "Allow shell: rm",
      session_id: "s1",
      risk: "high-risk",
      risk_tier: "deny",
    });
    expect(next).toHaveLength(1);
    expect(next[0].tool_name).toBe("run_terminal_cmd");
    const loaded = loadDenyHistory();
    expect(loaded.length).toBeGreaterThanOrEqual(1);
    expect(loaded[0].tool_name).toBe("run_terminal_cmd");
    expect(loaded[0].risk_tier).toBe("deny");
  });
});
