import { describe, expect, it } from "vitest";
import { groupTranscript } from "./SessionPane";
import type { TranscriptItem } from "../lib/protocol";

describe("groupTranscript tools", () => {
  it("batches consecutive tool calls so they render as tool_batch", () => {
    const items: TranscriptItem[] = [
      { kind: "user", text: "fix" },
      {
        kind: "tool",
        callId: "1",
        title: "read_file",
        status: "completed",
        output: "hi",
      },
      {
        kind: "tool",
        callId: "2",
        title: "grep",
        status: "completed",
        output: "x",
      },
      { kind: "assistant", text: "done" },
    ];
    const rows = groupTranscript(items);
    const batch = rows.find((r) => r.type === "tool_batch");
    expect(batch).toBeDefined();
    if (batch?.type !== "tool_batch") throw new Error("expected batch");
    expect(batch.tools).toHaveLength(2);
    expect(batch.tools.map((t) => t.item.title)).toEqual([
      "read_file",
      "grep",
    ]);
  });

  it("does not drop tools when interleaved with assistant text", () => {
    const items: TranscriptItem[] = [
      { kind: "user", text: "go" },
      {
        kind: "tool",
        callId: "a",
        title: "list_dir",
        status: "completed",
      },
      { kind: "assistant", text: "looking…" },
      {
        kind: "tool",
        callId: "b",
        title: "read_file",
        status: "running",
      },
    ];
    const rows = groupTranscript(items);
    const batches = rows.filter((r) => r.type === "tool_batch");
    expect(batches).toHaveLength(2);
    const toolCount = batches.reduce(
      (n, r) => n + (r.type === "tool_batch" ? r.tools.length : 0),
      0,
    );
    expect(toolCount).toBe(2);
  });
});
