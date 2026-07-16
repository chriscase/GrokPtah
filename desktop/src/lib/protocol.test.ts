import { describe, expect, it } from "vitest";
import { normalizeSessionUpdate, SLASH_COMMANDS } from "./protocol";

describe("normalizeSessionUpdate", () => {
  it("passes through internally tagged updates", () => {
    const u = normalizeSessionUpdate({
      type: "agent_message_chunk",
      session_id: "s1",
      text: "hi",
    });
    expect(u).toEqual({
      type: "agent_message_chunk",
      session_id: "s1",
      text: "hi",
    });
  });

  it("normalizes externally tagged serde shape", () => {
    const u = normalizeSessionUpdate({
      agent_thought_chunk: { session_id: "s1", text: "thinking" },
    });
    expect(u).toEqual({
      type: "agent_thought_chunk",
      session_id: "s1",
      text: "thinking",
    });
  });

  it("returns null for garbage", () => {
    expect(normalizeSessionUpdate(null)).toBeNull();
    expect(normalizeSessionUpdate(42)).toBeNull();
  });
});

describe("SLASH_COMMANDS", () => {
  it("includes core parity commands", () => {
    const cmds = SLASH_COMMANDS.map((c) => c.cmd);
    expect(cmds).toContain("/help");
    expect(cmds).toContain("/plan");
    expect(cmds).toContain("/yolo");
    expect(cmds).toContain("/model");
    expect(cmds).toContain("/effort");
    expect(cmds).toContain("/explore");
    expect(cmds).toContain("/sandbox");
    expect(cmds).toContain("/context");
  });
});
