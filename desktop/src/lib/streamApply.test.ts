import { describe, expect, it } from "vitest";
import {
  applyAssistantStreamChunk,
  nextAssistantText,
  shouldStickToBottom,
  streamVisualDelta,
} from "./streamApply";

describe("applyAssistantStreamChunk", () => {
  it("appends true deltas", () => {
    expect(nextAssistantText("Hello", " world")).toBe("Hello world");
  });

  it("replaces with cumulative full buffer", () => {
    expect(nextAssistantText("Hel", "Hello")).toBe("Hello");
    expect(nextAssistantText("Hello", "Hello world")).toBe("Hello world");
  });

  it("skips long exact redelivery; appends short repeated line", () => {
    const long = "x".repeat(100);
    expect(nextAssistantText(long, long)).toBe("skip");
    // short equal is legitimate second line, not multi-listener dump
    expect(nextAssistantText("Hi", "Hi")).toBe("HiHi");
  });

  it("does not drop legitimate repeated lines (no content-collapse)", () => {
    // Two identical code lines arriving as deltas must both remain.
    const line = "  return 1;\n";
    let t = "";
    t = nextAssistantText(t, line) as string;
    t = nextAssistantText(t, line) as string;
    expect(t).toBe(line + line);
  });

  it("seq drops stale chunks", () => {
    const r = applyAssistantStreamChunk("ab", "c", { seq: 2, lastSeq: 5 });
    expect(r.kind).toBe("skip");
    const r2 = applyAssistantStreamChunk("ab", "c", { seq: 6, lastSeq: 5 });
    expect(r2).toEqual({ kind: "append", text: "abc" });
  });

  it("handles empty / first chunk", () => {
    expect(nextAssistantText("", "")).toBe("skip");
    expect(nextAssistantText("", "Hi")).toBe("Hi");
  });
});

describe("streamVisualDelta", () => {
  it("returns suffix when next extends prev by content", () => {
    expect(streamVisualDelta("Hello", "Hello world")).toEqual({
      reset: false,
      added: " world",
    });
  });

  it("resets when text is not a pure extension (length-only would garble)", () => {
    // Same length replacement would break slice(prevLen) beams.
    const d = streamVisualDelta("abcXX", "abcYY");
    expect(d.reset).toBe(true);
    expect(d.added).toBe("abcYY");
  });

  it("no-op when equal", () => {
    expect(streamVisualDelta("same", "same")).toEqual({
      reset: false,
      added: "",
    });
  });
});

describe("shouldStickToBottom", () => {
  it("sticks when near bottom", () => {
    expect(shouldStickToBottom(0)).toBe(true);
    expect(shouldStickToBottom(40)).toBe(true);
    expect(shouldStickToBottom(80)).toBe(true);
  });
  it("does not stick when user scrolled up", () => {
    expect(shouldStickToBottom(120)).toBe(false);
    expect(shouldStickToBottom(500)).toBe(false);
  });
});
