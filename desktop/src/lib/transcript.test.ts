import { describe, expect, it } from "vitest";
import { entriesToTranscriptItems } from "./transcript";

describe("entriesToTranscriptItems", () => {
  it("maps tool rows with title/status/output", () => {
    const items = entriesToTranscriptItems([
      { role: "user", text: "hi" },
      {
        role: "tool",
        text: "read_file · completed",
        tool_call_id: "c1",
        tool_title: "read_file",
        tool_status: "completed",
        tool_output: "file body",
      },
      { role: "assistant", text: "done" },
    ]);
    expect(items).toHaveLength(3);
    expect(items[1]).toMatchObject({
      kind: "tool",
      callId: "c1",
      title: "read_file",
      status: "completed",
      output: "file body",
    });
    expect(items[0].kind).toBe("user");
    expect(items[2].kind).toBe("assistant");
  });

  it("does not coerce tools into assistant bubbles", () => {
    const items = entriesToTranscriptItems([
      {
        role: "tool",
        text: "grep · completed",
        tool_title: "grep",
        tool_status: "completed",
      },
    ]);
    expect(items[0].kind).toBe("tool");
  });

  it("hydrates thought role so reasoning survives reload (#149)", () => {
    const items = entriesToTranscriptItems([
      { role: "user", text: "why?" },
      { role: "thought", text: "consider the options…" },
      { role: "assistant", text: "because" },
    ]);
    expect(items[1]).toEqual({
      kind: "thought",
      text: "consider the options…",
    });
  });
});

